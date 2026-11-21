// AC-1700 (#1768): threshold checks and metric extraction are pure; no Cargo, git or showcase runs.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { check, flowMetrics, gapMetrics, limitOf, parseThresholds, report, scaleMetrics, simpleloginManifest, simpleloginMetrics } from './showcase-watch.mjs';

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const thresholds = (metrics) => ({ metrics });

test('a min threshold may not fall below baseline x (1 - tolerance); a max may not rise above baseline x (1 + tolerance)', () => {
  assert.equal(limitOf({ baseline: 100, direction: 'min', tolerance: 0.05 }), 95);
  assert.equal(limitOf({ baseline: 60, direction: 'max', tolerance: 1.5 }), 150);
  const rules = thresholds({
    endpoints: { baseline: 426, direction: 'min', tolerance: 0 },
    gap_nodes: { baseline: 100, direction: 'max', tolerance: 0.05 },
    wall_seconds: { baseline: 60, direction: 'max', tolerance: 1 },
  });
  const held = check({ endpoints: 426, gap_nodes: 105, wall_seconds: 30, extra: 7 }, rules);
  assert.equal(held.passed, true);
  assert.deepEqual(held.rows.map((row) => row.status), ['held', 'held', 'better', 'untracked']);
  const missed = check({ endpoints: 425, gap_nodes: 106, wall_seconds: 121 }, rules);
  assert.equal(missed.passed, false);
  assert.deepEqual(missed.rows.map((row) => row.status), ['missed', 'missed', 'missed']);
});

test('a tracked metric the run did not measure is a miss, and the report says so with the measured commit', () => {
  const result = check({}, thresholds({ endpoints: { baseline: 1, direction: 'min', tolerance: 0 } }));
  assert.equal(result.passed, false);
  const text = report('simplelogin', result, { measuredAt: { commit: 'abc', version: 'v0.72.0', platform: 'darwin-arm64' } });
  assert.match(text, /Measured at Cartograph `abc` \(v0\.72\.0\) on darwin-arm64\./u);
  assert.match(text, /\| `endpoints` \| — \| 1 \| ≥ 1 \| \*\*not measured\*\* \|/u);
  assert.match(text, /1 threshold\(s\) missed/u);
});

test('threshold files are validated, and the shipped ones parse', () => {
  assert.throws(() => parseThresholds('{"metrics":{"x":{"baseline":1,"direction":"up","tolerance":0}}}'), /direction/u);
  assert.throws(() => parseThresholds('{"metrics":{"x":{"baseline":"1","direction":"min","tolerance":0}}}'), /baseline/u);
  assert.throws(() => parseThresholds('{"metrics":{"x":{"baseline":1,"direction":"min","tolerance":-1}}}'), /tolerance/u);
  for (const lane of ['simplelogin', 'scale']) {
    const file = parseThresholds(readFileSync(path.join(root, 'bench/thresholds', `${lane}.json`), 'utf8'));
    assert.ok(Object.keys(file.metrics).length > 0, lane);
    assert.match(file.measured_at.commit, /^[0-9a-f]{8,40}$/u, lane);
  }
  const simplelogin = JSON.parse(readFileSync(path.join(root, 'bench/thresholds/simplelogin.json'), 'utf8'));
  assert.deepEqual(simplelogin.lane.sources.map((source) => source.name), ['app', 'browser-extension', 'Simple-Login-Android']);
  for (const source of simplelogin.lane.sources) assert.match(source.commit, /^[0-9a-f]{40}$/u);
});

test('the SimpleLogin system manifest binds the clients to the server', () => {
  const text = simpleloginManifest([{ name: 'app' }, { name: 'browser-extension' }], { 'SETTINGS.apiUrl': 'https://app.simplelogin.io' });
  assert.equal(text, [
    '[[repos]]', 'url = "app"', '[[repos]]', 'url = "browser-extension"', '',
    '[env]', '"SETTINGS.apiUrl" = "https://app.simplelogin.io"', '',
    '[[services]]', 'name = "simplelogin"', 'repo = "app"', 'base_urls = ["https://app.simplelogin.io"]', '',
  ].join('\n'));
});

test('gap findings are counted per class and per extractor, and flows by verdict', () => {
  const gaps = gapMetrics({ classes: [
    { extractor: 't0.adapter-python', members: ['a', 'b', 'c'] },
    { extractor: 't0.adapter-ts', members: ['d'] },
    { extractor: 't0.adapter-python', instances: 2 },
  ] });
  assert.deepEqual(gaps, { gap_findings: 6, gap_classes: 3, 'gap_findings.t0.adapter-python': 5, 'gap_findings.t0.adapter-ts': 1 });
  const flows = flowMetrics('# Flow dossier\n\n## A — Verified (score 1.00)\n\n## B — Partial (score 0.5)\n\n## C — Inferred (score 0.2)\n\n## Assertions and inline provenance\n');
  assert.deepEqual(flows, { flows_traced: 3, flows_verified: 1, flows_partial: 1 });
});

test('SimpleLogin metrics are read from the showcase outputs and the answer key', async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'showcase-watch-'));
  try {
    await mkdir(path.join(dir, 'verified-only'));
    const endpoint = (method, route) => ({ id: `${method} ${route}`, props: { method, path: route } });
    await writeFile(path.join(dir, 'labels.json'), JSON.stringify({ Endpoint: 2, Gap: 9, BusinessRule: 4, DataEntity: 1, TestCase: 3 }));
    await writeFile(path.join(dir, 'endpoints.json'), JSON.stringify([endpoint('GET', '/api/aliases'), endpoint('POST', '/login')]));
    await writeFile(path.join(dir, 'fetches.json'), JSON.stringify([{ reason: null }, { reason: null }, { reason: 'runtime-computed fetch URL' }]));
    await writeFile(path.join(dir, 'verified-only', 'flow_dossiers.md'), '## A — Verified (score 1)\n');
    await writeFile(path.join(dir, 'verified-only', 'gap_register.json'), JSON.stringify({ classes: [{ extractor: 't0.adapter-python', members: ['x'] }] }));
    const answerKey = path.join(dir, 'api.md');
    await writeFile(answerKey, '#### GET /api/aliases\n\n#### GET /api/mailboxes\n');
    const metrics = simpleloginMetrics(dir, { answerKey, timing: { wall_seconds: 12.5, max_rss_bytes: 1024 } });
    assert.equal(metrics.endpoints, 2);
    assert.equal(metrics.api_documented_matched, 1);
    assert.equal(metrics.client_calls_linked, 2);
    assert.equal(metrics.client_calls_gap, 1);
    assert.equal(metrics.flows_verified, 1);
    assert.equal(metrics.gap_findings, 1);
    assert.equal(metrics.wall_seconds, 12.5);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('scale metrics are keyed by repository, and unknown measurements are left out', () => {
  const metrics = scaleMetrics({ repos: [{
    name: 'kafka',
    counts: { files: 3, nodes: 10, edges: 20, labels: { Endpoint: 43, Gap: 5 } },
    measurements: { wall_seconds: 70.4, max_rss_bytes: null },
  }] });
  assert.deepEqual(metrics, {
    'kafka.files': 3, 'kafka.nodes': 10, 'kafka.edges': 20, 'kafka.endpoints': 43,
    'kafka.business_rules': 0, 'kafka.gap_nodes': 5, 'kafka.wall_seconds': 70.4,
  });
});
