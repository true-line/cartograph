#!/usr/bin/env node
// Local scale benchmark (#1228, AC-0423). Builds the release showcase test
// binary once, recovers each pinned repository in `bench/repos.toml` through
// the production ingest path, and writes one JSON result per run: wall time
// and peak memory as measurements, and graph, endpoint and receipt counts as
// deterministic facts. Local only: never run in CI. It reports and never
// fails on a threshold; a counts difference between two runs this script built
// from one clean commit, for the same url, commit and path, is the only finding
// `--compare` treats as a determinism failure; it also fails when the two
// results do not cover the same repositories.
//
//   node scripts/bench-ingest.mjs --work <dir> [--manifest bench/repos.toml] [--bin <app test binary>] [--only <name>]
//   node scripts/bench-ingest.mjs --compare <before.json> <after.json>
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const TEST = 'showcase_tests::showcase_recover_and_export';

/** The `[[repo]]` tables of a manifest: `name`, `url`, an exact commit and an optional `path` inside it. */
export function parseManifest(text) {
  const repos = [];
  for (const raw of text.split('\n')) {
    const line = raw.replace(/#.*$/, '').trim();
    if (!line) continue;
    if (line === '[[repo]]') { repos.push({}); continue; }
    const field = /^(name|url|commit|path)\s*=\s*"([^"\\]*)"$/.exec(line);
    if (!field || !repos.length) throw new Error(`unsupported manifest line: ${raw}`);
    repos.at(-1)[field[1]] = field[2];
  }
  const names = new Set();
  for (const repo of repos) {
    if (!/^[a-z0-9][a-z0-9-]{0,63}$/.test(repo.name ?? '')) throw new Error(`invalid repo name: ${repo.name}`);
    if (!/^https:\/\/[^\s@]+$/.test(repo.url ?? '')) throw new Error(`${repo.name}: url must be https without credentials`);
    if (!/^[0-9a-f]{40}$/.test(repo.commit ?? '')) throw new Error(`${repo.name}: commit must be a full sha`);
    if (repo.path !== undefined && !/^[\w.-]+(\/[\w.-]+)*$/.test(repo.path)) throw new Error(`${repo.name}: path must be relative`);
    if (repo.path?.split('/').some((part) => part === '.' || part === '..')) throw new Error(`${repo.name}: path must be relative`);
    if (names.has(repo.name)) throw new Error(`duplicate repo name: ${repo.name}`);
    names.add(repo.name);
  }
  return repos;
}

/** Wall seconds and memory bytes from `/usr/bin/time -l` (macOS) or `-v` (Linux). */
export function parseTime(stderr, platform) {
  const number = (pattern) => {
    const found = pattern.exec(stderr);
    return found ? Number(found[1]) : null;
  };
  if (platform === 'darwin') {
    return {
      wall_seconds: number(/([\d.]+) real/),
      max_rss_bytes: number(/(\d+)\s+maximum resident set size/),
      peak_footprint_bytes: number(/(\d+)\s+peak memory footprint/),
    };
  }
  const wall = /Elapsed \(wall clock\) time \(h:mm:ss or m:ss\): (?:(\d+):)?(\d+):([\d.]+)/.exec(stderr);
  const kib = number(/Maximum resident set size \(kbytes\): (\d+)/);
  return {
    wall_seconds: wall ? Number(wall[1] ?? 0) * 3600 + Number(wall[2]) * 60 + Number(wall[3]) : null,
    max_rss_bytes: kib === null ? null : kib * 1024,
    peak_footprint_bytes: null,
  };
}

/** Read-only sqlite query through the `sqlite3` CLI; `null` when unavailable. */
function sqlite(file, sql) {
  if (!existsSync(file)) return null;
  const run = spawnSync('sqlite3', ['-readonly', '-json', file, sql], { encoding: 'utf8' });
  if (run.status !== 0) return null;
  return JSON.parse(run.stdout || '[]');
}

/** The deterministic counts of one showcase output directory. */
export function summarize(output, query = sqlite) {
  const read = (name) => JSON.parse(readFileSync(path.join(output, name), 'utf8'));
  const ingest = read('ingest.json').map(({ summary }) => summary);
  const auth = {};
  for (const endpoint of read('endpoints.json')) {
    const state = endpoint.props?.auth?.state ?? 'absent';
    auth[state] = (auth[state] ?? 0) + 1;
  }
  const data = path.join(output, 'profile', 'data');
  const count = (file, sql) => query(path.join(data, file), sql)?.[0] ?? null;
  const receipts = count('retained-source/receipts.sqlite',
    'select count(*) as receipts, coalesce(sum(length(cast(payload as blob))), 0) as payload_bytes from receipts');
  const headers = count('retained-source/receipts.sqlite',
    'select count(*) as headers, coalesce(sum(length(cast(header as blob))), 0) as header_bytes from receipt_headers');
  const omissions = count('graph.db',
    'select (select count(*) from source_node_omissions) as nodes, (select count(*) from source_edge_omissions) as edges');
  return {
    files: ingest.reduce((sum, summary) => sum + summary.files, 0),
    nodes: ingest.reduce((sum, summary) => sum + summary.nodes, 0),
    edges: ingest.reduce((sum, summary) => sum + summary.edges, 0),
    labels: sorted(read('labels.json')),
    endpoint_auth: sorted(auth),
    receipts: receipts && headers ? { ...receipts, ...headers } : null,
    receipt_omissions: omissions,
  };
}

function sorted(object) {
  return Object.fromEntries(Object.entries(object).sort(([a], [b]) => a.localeCompare(b)));
}

/**
 * Two results this script built from one clean Cartograph commit: only then
 * must counts match. A `--bin` binary has no verified provenance.
 */
function sameBuild(before, after) {
  return before.cartograph_commit === after.cartograph_commit && !before.cartograph_dirty && !after.cartograph_dirty
    && before.build === 'cargo' && after.build === 'cargo';
}

/** The fields that select what a repository result recovered. */
const SOURCE = ['url', 'commit', 'path'];

/**
 * Lines describing how `after` differs from `before`, whether any count
 * differs, and whether one does for the same build and the same source.
 */
export function compare(before, after) {
  const lines = [];
  let countsDiffer = false;
  let nondeterministic = false;
  let membershipDiffers = false;
  const build = sameBuild(before, after);
  const byName = new Map(before.repos.map((repo) => [repo.name, repo]));
  for (const repo of after.repos) {
    const old = byName.get(repo.name);
    byName.delete(repo.name);
    if (!old) { lines.push(`${repo.name}: new`); membershipDiffers = true; continue; }
    for (const key of SOURCE) {
      if (old[key] !== repo[key]) lines.push(`${repo.name}: ${key} moved ${old[key] ?? '(none)'} -> ${repo[key] ?? '(none)'}`);
    }
    for (const key of ['wall_seconds', 'max_rss_bytes', 'peak_footprint_bytes']) {
      const [a, b] = [old.measurements[key], repo.measurements[key]];
      if (a !== null && b !== null && a > 0) lines.push(`${repo.name}: ${key} ${a} -> ${b} (${((b - a) / a * 100).toFixed(1)}%)`);
    }
    if (JSON.stringify(old.counts) !== JSON.stringify(repo.counts)) {
      countsDiffer = true;
      if (build && SOURCE.every((key) => old[key] === repo[key])) nondeterministic = true;
      lines.push(`${repo.name}: counts differ ${JSON.stringify(old.counts)} -> ${JSON.stringify(repo.counts)}`);
    }
  }
  for (const name of byName.keys()) { lines.push(`${name}: removed`); membershipDiffers = true; }
  if (nondeterministic) {
    lines.push('determinism: counts differ between two runs of the same build and source');
  }
  return { lines, countsDiffer, nondeterministic, membershipDiffers };
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: 'utf8', maxBuffer: 1 << 30, ...options });
  if (result.status !== 0) throw new Error(`${command} ${args.join(' ')} failed:\n${result.stderr}`.trimEnd());
  return result;
}

function build() {
  const out = run('cargo', ['test', '--release', '-p', 'app', '--bin', 'app', '--no-run', '--message-format=json'], { cwd: ROOT });
  const executable = out.stdout.split('\n').filter(Boolean).map((line) => JSON.parse(line))
    .find((message) => message.reason === 'compiler-artifact' && message.target?.name === 'app' && message.profile?.test)?.executable;
  if (!executable) throw new Error('cargo reported no app test executable');
  return executable;
}

/** Whether `child` is `parent` or inside it, compared as given. */
export function contained(parent, child) {
  const relative = path.relative(parent, child);
  return relative === '' || (relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
}

/** The real path of `target`, resolving its nearest existing ancestor. */
export function realPath(target) {
  const missing = [];
  let existing = path.resolve(target);
  while (!existsSync(existing)) {
    missing.unshift(path.basename(existing));
    existing = path.dirname(existing);
  }
  return path.join(realpathSync(existing), ...missing);
}

/** The real directory `relative` selects in `checkout`; a symlink may not leave it. */
export function selectRoot(checkout, relative) {
  const root = realpathSync(relative ? path.join(checkout, relative) : checkout);
  if (!contained(realpathSync(checkout), root) || !statSync(root).isDirectory()) {
    throw new Error(`${relative} does not select a directory inside ${checkout}`);
  }
  return root;
}

/** A clean checkout of `repo` at its exact commit, and the selected root in it; existing checkouts are verified, not reset. */
function checkout(repo, work) {
  const dir = path.join(work, 'sources', repo.name);
  if (!existsSync(dir)) {
    run('git', ['clone', '--quiet', '--no-checkout', repo.url, dir]);
    run('git', ['-C', dir, 'checkout', '--quiet', '--detach', repo.commit]);
  }
  const head = run('git', ['-C', dir, 'rev-parse', 'HEAD']).stdout.trim();
  const dirty = run('git', ['-C', dir, 'status', '--porcelain']).stdout.trim();
  if (head !== repo.commit || dirty) throw new Error(`${dir} is not a clean checkout of ${repo.commit}`);
  return selectRoot(dir, repo.path);
}

function parseArguments(argv) {
  const options = { manifest: path.join(ROOT, 'bench', 'repos.toml') };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === '--compare') { options.compare = [argv[i + 1], argv[i + 2]]; i += 2; }
    else if (['--work', '--manifest', '--bin', '--only'].includes(flag) && argv[i + 1]) { options[flag.slice(2)] = argv[i + 1]; i += 1; }
    else throw new Error(`unknown argument: ${flag}`);
  }
  if (!options.compare && !options.work) throw new Error('--work <dir> or --compare <before> <after> is required');
  return options;
}

function main() {
  const options = parseArguments(process.argv.slice(2));
  if (options.compare) {
    const [before, after] = options.compare.map((file) => JSON.parse(readFileSync(file, 'utf8')));
    const { lines, nondeterministic, membershipDiffers } = compare(before, after);
    console.log(lines.join('\n'));
    // A comparison that does not cover the same repositories proves nothing.
    process.exitCode = nondeterministic || membershipDiffers ? 1 : 0;
    return;
  }
  if (!['darwin', 'linux'].includes(process.platform)) throw new Error('the benchmark supports macOS and Linux only');
  const repos = parseManifest(readFileSync(options.manifest, 'utf8')).filter((repo) => !options.only || repo.name === options.only);
  const work = path.resolve(options.work);
  // Output inside the checkout would make the next run's tree uncommitted.
  // The real path is checked before anything is created, so a symlink into
  // the checkout is refused too.
  if (contained(realpathSync(ROOT), realPath(work))) throw new Error('--work must be outside the Cartograph checkout');
  mkdirSync(work, { recursive: true });
  // Results from an uncommitted tree, or one that changed during the build,
  // are not comparable by commit.
  const tree = () => [run('git', ['rev-parse', 'HEAD'], { cwd: ROOT }).stdout.trim(),
    run('git', ['status', '--porcelain'], { cwd: ROOT }).stdout.trim()];
  const [commit, status] = tree();
  const binary = options.bin ? path.resolve(options.bin) : build();
  const dirty = status !== '' || tree().join('\n') !== [commit, status].join('\n');
  const stamp = new Date().toISOString().replace(/[:.]/g, '-');
  const result = { schema_version: 1, cartograph_commit: commit, cartograph_dirty: dirty, build: options.bin ? 'supplied' : 'cargo', platform: `${process.platform}-${process.arch}`, started_at: stamp, repos: [] };
  for (const repo of repos) {
    const source = checkout(repo, work);
    const output = path.join(work, 'runs', stamp, repo.name);
    mkdirSync(path.dirname(output), { recursive: true });
    const timed = spawnSync('/usr/bin/time', [process.platform === 'darwin' ? '-l' : '-v', binary, '--ignored', '--exact', TEST], {
      cwd: path.join(ROOT, 'src-tauri'), encoding: 'utf8', maxBuffer: 1 << 30,
      env: { ...process.env, CARTOGRAPH_SHOWCASE_SOURCES: source, CARTOGRAPH_SHOWCASE_OUTPUT: output },
    });
    if (timed.status !== 0) throw new Error(`${repo.name}: showcase failed\n${timed.stdout}\n${timed.stderr}`.trimEnd());
    result.repos.push({ ...repo, measurements: parseTime(timed.stderr, process.platform), counts: summarize(output) });
    console.log(`${repo.name}: ${JSON.stringify(result.repos.at(-1).measurements)}`);
  }
  const file = path.join(work, `bench-${stamp}.json`);
  writeFileSync(file, `${JSON.stringify(result, null, 2)}\n`);
  console.log(file);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try { main(); } catch (error) { console.error(error.message); process.exitCode = 2; }
}
